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
        items: spec.windows.map((days) => ({
          label: days >= 365 ? 'A year' : `${days} days`,
          checked: widget.days === days,
          run: () => overview.rewindow(widget.id, days),
        })),
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
  oncontextmenu={(e) => menu.show(e, cardMenu())}
>
  <header>
    {#if overview.editing}
      <!-- The grip is the drag handle and says so. `draggable` is set on the
           card itself by the grid, because a drag started from a button
           inside a text field would otherwise select text instead. -->
      <span class="grip" title="Drag to move"><Icon name="grip" size={14} /></span>
    {/if}
    <h2>{spec.label}</h2>
    <span class="spacer"></span>
    {#if tools}<div class="tools">{@render tools()}</div>{/if}
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
          <Icon name="chevron" size={13} />
        </button>
        {#if spec.sizes.length > 1}
          <div class="sizes" role="group" aria-label="Width">
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
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-raised);
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
    box-shadow: inset 3px 0 0 var(--accent);
  }

  header {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-height: 22px;
    margin-bottom: var(--sp-3);
  }

  h2 {
    margin: 0;
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
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
</style>
