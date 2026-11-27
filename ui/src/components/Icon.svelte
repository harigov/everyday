<script lang="ts" module>
  import { ICONS, type IconName } from '../lib/icons'

  /**
   * Each icon's artwork, parsed once and cloned for every icon drawn.
   *
   * An `{@html}` that is an element's only child is an `innerHTML` assignment
   * on every mount, and inside an `<svg>` that is a full markup parse, which
   * WebKit's quick path for HTML does not cover. A shelf of rated cards drew
   * thousands on the way in; cloning nodes already parsed is a fraction of
   * that, and the same DOM. Parsed inside an `<svg>`, so the nodes are in the
   * SVG namespace.
   */
  const parsed = new Map<IconName, DocumentFragment>()

  function artwork(name: IconName): DocumentFragment | null {
    // An own property only: a name that is not an icon draws nothing, as an
    // empty `{@html}` did, and is not kept.
    if (!Object.hasOwn(ICONS, name)) return null
    let art = parsed.get(name)
    if (!art) {
      const holder = document.createElementNS('http://www.w3.org/2000/svg', 'svg')
      holder.innerHTML = ICONS[name]
      art = document.createDocumentFragment()
      art.append(...holder.childNodes)
      parsed.set(name, art)
    }
    return art.cloneNode(true) as DocumentFragment
  }

  /** Put `name`'s artwork in the icon, and swap it when the name changes in
   *  place -- a task's tick going from a circle to a check, say. */
  function draw(node: SVGSVGElement, name: IconName) {
    const paint = (next: IconName) => {
      const art = artwork(next)
      if (art) node.replaceChildren(art)
      else node.replaceChildren()
    }
    paint(name)
    return { update: paint }
  }
</script>

<script lang="ts">
  let {
    name,
    size = 16,
    weight = 1.5,
    filled = false,
  }: {
    name: IconName
    /** Rendered edge length in CSS pixels. */
    size?: number
    /** Stroke thickness in CSS pixels, held constant across sizes. */
    weight?: number
    /** Fill the shape instead of stroking it -- a starred entry, say. */
    filled?: boolean
  } = $props()

  // The artwork is drawn on a 24-unit grid, so a stroke of `w` units renders
  // at `w * size / 24` pixels. Solving that for a constant on-screen weight
  // is what keeps a 16px sidebar icon and a 19px toolbar icon looking like
  // they came from the same family -- scaling the viewBox alone would make
  // the smaller one visibly lighter.
  const strokeWidth = $derived((weight * 24) / size)
</script>

<svg
  class="icon"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill={filled ? 'currentColor' : 'none'}
  stroke={filled ? 'none' : 'currentColor'}
  stroke-width={strokeWidth}
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
  focusable="false"
  use:draw={name}
></svg>

<style>
  /* Icons sit inline with text more often than not; this keeps them centred
     on the cap height rather than hanging off the baseline. */
  .icon {
    display: block;
    flex: none;
    overflow: visible;
  }
</style>
