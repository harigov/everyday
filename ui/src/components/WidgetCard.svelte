<script lang="ts">
  // The frame every widget is drawn in: a heading, and the handles for
  // moving, sizing and removing it.
  //
  // The handles appear only while the page is being arranged. A dashboard is
  // read far more often than it is edited, and a permanent row of grips and
  // crosses on every card turns a page you glance at into a form you have to
  // pick your way through. Everything the handles do is also on the card's
  // right-click menu, which is available whether or not the page is in edit
  // mode -- the same arrangement every list in this application uses, and the
  // reason none of this is mouse-only.
  //
  // Dragging is native HTML drag-and-drop rather than pointer maths. It is
  // the shape the platform already knows -- a drag image, a drop cursor, an
  // escape that cancels -- and the keyboard path does not depend on it: the
  // menu's "Move earlier" and "Move later" do the same thing, which is what
  // makes this reachable without a pointer at all.

  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { overview } from '../lib/overview.svelte'
  import { phone } from '../lib/phone.svelte'
  import { SIZE_LABELS, specOf, WIDGET_SIZES, type Widget } from '../lib/dashboard'
  import { tracking } from '../lib/tracking.svelte'
  import Icon from './Icon.svelte'

  interface Props {
    widget: Widget
    /** Its place on the page, for "is this the first one". */
    index: number
    count: number
    /** The card whose edge the pointer is over, while something is dragged. */
    dragOver?: boolean
    children: import('svelte').Snippet
    /** Drawn in the header, to the left of the handles. A picker, a count. */
    tools?: import('svelte').Snippet
  }

  const { widget, index, count, dragOver = false, children, tools }: Props = $props()

  const spec = $derived(specOf(widget.type))

  function windowLabel(days: number): string {
    return days >= 365 ? 'A year' : `${days} days`
  }

  /** The window choices, for the card menu and the chip in the header alike. */
  function windowItems(): MenuItem[] {
    return (spec.windows ?? []).map((days) => ({
      label: windowLabel(days),
      checked: widget.days === days,
      run: () => overview.rewindow(widget.id, days),
    }))
  }

  /** Open the window choices under the chip, rather than at the pointer. */
  function pickWindow(e: MouseEvent) {
    const box = (e.currentTarget as HTMLElement).getBoundingClientRect()
    menu.showAt(box.left, box.bottom + 4, windowItems())
  }

  function cardMenu(): MenuItem[] {
    return tidyMenu([
      {
        label: 'Move earlier',
        icon: 'chevron',
        disabled: index === 0,
        run: () => overview.move(widget.id, -1),
      },
      {
        label: 'Move later',
        icon: 'chevron',
        disabled: index === count - 1,
        run: () => overview.move(widget.id, 1),
      },
      SEP,
      !phone.on &&
        spec.sizes.length > 1 && {
          label: 'Width',
          icon: 'grid',
          items: WIDGET_SIZES.filter((s) => spec.sizes.includes(s)).map((size) => ({
            label: SIZE_LABELS[size],
            checked: widget.size === size,
            run: () => overview.resize(widget.id, size),
          })),
        },
      spec.subject === 'tracker' && {
        label: 'Which tracker',
        icon: 'refresh',
        items: tracking.live.map((t) => ({
          label: t.name,
          dot: t.color,
          checked: widget.subject === t.id,
          run: () => overview.retarget(widget.id, t.id),
        })),
      },
      spec.windows && {
        label: 'Window',
        icon: 'calendar',
        items: windowItems(),
      },
      SEP,
      {
        label: overview.editing ? 'Stop arranging' : 'Arrange this page',
        icon: 'grip',
        run: () => (overview.editing = !overview.editing),
      },
      {
        label: 'Take it off the page',
        icon: 'close',
        danger: true,
        run: () => overview.remove(widget.id),
      },
    ])
  }
</script>

<section
  class="card"
  class:editing={overview.editing}
  class:over={dragOver}
  data-widget={widget.id}
  aria-label={spec.label}
  style="--tint: var(--app-{spec.app})"
  oncontextmenu={(e) => menu.show(e, cardMenu())}
>
  <header>
    {#if overview.editing}
      <!-- The grip is the drag handle and says so. `draggable` is set on the
           card itself by the grid, because a drag started from a button
           inside a text field would otherwise select text instead. -->
      <span class="grip desktop-only" title="Drag to move"><Icon name="grip" size={14} /></span>
    {/if}
    <!-- The card's own mark, in the colour of the app it draws from -- the
         app bar's colour for that app, so a glance at the page says which
         card is mail and which is the calendar before a word is read. -->
    <span class="mark" aria-hidden="true"><Icon name={spec.icon} size={15} weight={1.8} /></span>
    <h2>{spec.label}</h2>
    <span class="spacer"></span>
    {#if tools}<div class="tools">{@render tools()}</div>{/if}
    {#if spec.windows && widget.days !== null}
      <!-- The window on show, always: "who sends you the most" means
           nothing without "over what". The same choices as the card menu. -->
      <button class="window" title="Change the window" aria-haspopup="menu" onclick={pickWindow}>
        {windowLabel(widget.days)}
        <span class="down"><Icon name="chevron" size={10} /></span>
      </button>
    {/if}
    {#if overview.editing}
      <div class="handles">
        <button
          class="handle"
          title="Move earlier"
          aria-label="Move earlier"
          disabled={index === 0}
          onclick={() => overview.move(widget.id, -1)}
        >
          <span class="back"><Icon name="chevron" size={13} /></span>
        </button>
        <button
          class="handle"
          title="Move later"
          aria-label="Move later"
          disabled={index === count - 1}
          onclick={() => overview.move(widget.id, 1)}
        >
          <span class="fwd"><Icon name="chevron" size={13} /></span>
        </button>
        {#if spec.sizes.length > 1}
          <div class="sizes desktop-only" role="group" aria-label="Width">
            {#each WIDGET_SIZES.filter((s) => spec.sizes.includes(s)) as size (size)}
              <button
                class="handle wide"
                class:on={widget.size === size}
                title={SIZE_LABELS[size]}
                aria-pressed={widget.size === size}
                onclick={() => overview.resize(widget.id, size)}
              >
                {SIZE_LABELS[size]}
              </button>
            {/each}
          </div>
        {/if}
        <button
          class="handle danger"
          title="Take it off the page"
          aria-label="Take {spec.label} off the page"
          onclick={() => overview.remove(widget.id)}
        >
          <Icon name="close" size={13} />
        </button>
      </div>
    {/if}
  </header>

  <div class="body">{@render children()}</div>
</section>

<style>
  .card {
    display: flex;
    flex-direction: column;
    /* Fills its grid slot, so every card in a row is as tall as the tallest
       one in it -- see `.slot` in `OverviewView`. */
    flex: 1;
    min-width: 0;
    padding: var(--sp-4) var(--sp-5) var(--sp-5);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    /* Raised off the page, with a wash of the card's own colour across its
       top that has faded out before the content starts: enough to tell the
       cards in a row apart, not enough to colour what is written in them. */
    background:
      linear-gradient(color-mix(in oklab, var(--tint) 10%, transparent), transparent 88px),
      var(--bg-raised);
    /* The glass rim the frame's own panels wear, and nothing in a flat
       theme. Inset and unblurred on purpose: a blurred shadow on each of a
       page of cards is redrawn for every frame of a scroll in WebKitGTK
       (see `ItemCard`'s covers). */
    box-shadow: var(--edge);
  }

  .card.editing {
    /* A dashed outline while arranging, so the cards read as things that
       can be picked up rather than as the finished page. */
    border-style: dashed;
    border-color: var(--border-strong);
  }

  /* Where a dragged card would land. A line rather than a shifted layout:
     re-flowing the whole grid under the pointer makes the target move. */
  .card.over {
    box-shadow:
      inset 3px 0 0 var(--accent),
      var(--edge);
  }

  header {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-height: 28px;
    margin-bottom: var(--sp-4);
  }

  /* The card's mark: its icon in its app's colour on a tint of the same,
     the way the app bar draws the app that is open. */
  .mark {
    display: grid;
    flex: none;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 9px;
    background: color-mix(in oklab, var(--tint) 18%, transparent);
    color: var(--tint);
  }

  /* A heading that reads as one: the card's name at body size in the text
     colour. It was the faint small capitals the sidebar's section labels
     use, which on a page of nothing but cards left every card's name the
     quietest thing on it. */
  h2 {
    margin: 0;
    color: var(--fg);
    font-size: var(--text-base);
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .spacer {
    flex: 1;
  }

  .grip {
    display: grid;
    place-items: center;
    color: var(--fg-faint);
    cursor: grab;
  }

  .window {
    display: inline-flex;
    flex: none;
    align-items: center;
    gap: 3px;
    height: 24px;
    padding: 0 var(--sp-2) 0 var(--sp-3);
    border: 1px solid var(--border);
    border-radius: 999px;
    color: var(--fg-subtle);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .window:hover {
    background: var(--bg-hover);
    color: var(--fg-muted);
  }
  /* The icon set's one chevron points right; a menu opens downwards. */
  .down {
    display: grid;
    place-items: center;
    rotate: 90deg;
  }

  .tools,
  .handles {
    display: flex;
    align-items: center;
    gap: 2px;
  }

  .handle {
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: var(--radius-sm);
    color: var(--fg-subtle);
  }
  .handle.wide {
    width: auto;
    padding: 0 var(--sp-2);
    font-size: var(--text-xs);
    font-weight: 550;
  }
  .handle:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .handle:disabled {
    opacity: 0.35;
  }
  .handle.on {
    background: var(--bg-active);
    color: var(--fg);
  }
  .handle.danger:hover {
    background: color-mix(in oklab, var(--danger) 14%, transparent);
    color: var(--danger);
  }

  .sizes {
    display: flex;
    gap: 2px;
    margin: 0 var(--sp-1);
    padding: 0 var(--sp-1);
    border-left: 1px solid var(--border);
    border-right: 1px solid var(--border);
  }

  /* A chevron pointing back. The icon set has one direction and this is the
     same trick the entry list's week arrows use. */
  .back {
    display: grid;
    place-items: center;
    rotate: 180deg;
  }

  .body {
    flex: 1;
    min-width: 0;
  }
  /* A card that is one figure -- due today, on now -- holds it halfway down
     rather than at the top: stretched to the height of a long list beside
     it, it read as a line over an empty card. `StatTile` marks itself a
     figure for this, rather than this knowing its class name. */
  .body:has(> :global([data-figure]:only-child)) {
    display: flex;
    flex-direction: column;
    justify-content: center;
  }

  /* A phone: a column of cards, each a little less padded so the figures in
     it have the room, and no drag handle or width choice -- one card is the
     whole width -- but the arrows and the cross, which move and remove a
     card by tap, a size a fingertip can hit. */
  :global([data-phone]) .card {
    padding: var(--sp-4);
  }
  :global([data-phone]) header {
    margin-bottom: var(--sp-3);
  }
  :global([data-phone]) .window {
    height: 32px;
    padding: 0 var(--sp-3);
    font-size: var(--text-sm);
  }
  :global([data-phone]) .handles {
    gap: var(--sp-1);
  }
  :global([data-phone]) .handle {
    width: 38px;
    height: 38px;
  }
  /* One column: earlier is up and later is down. */
  :global([data-phone]) .back {
    rotate: -90deg;
  }
  :global([data-phone]) .fwd {
    display: grid;
    place-items: center;
    rotate: 90deg;
  }
</style>
