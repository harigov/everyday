<script lang="ts">
  // The Overview: a page of widgets somebody arranged themselves.
  //
  // It was four fixed panes -- Today, This week, Goals, Habits -- and each of
  // them was somebody's guess at what mattered. The guesses were not bad, but
  // they were one guess for everybody: the person keeping a medication log
  // and the person keeping a reading list were shown the same four screens,
  // and neither could put the one number they open the app for at the top.
  // The panes are widgets now and the page is a grid you fill yourself.
  //
  // Two of the four panes did not become widgets, and both left for a better
  // address rather than being dropped. Goals are edited in the todo app,
  // beside the tasks that make them happen; roles are defined in Settings,
  // under About You, with the rest of what this vault knows about its owner.
  // What is left here is the reading, which is what an overview is.
  //
  // The grid is six columns wide. Widgets span two, three or six of them, so
  // every combination tiles without a hole -- see `dashboard.ts`. Below a
  // narrow width it collapses to three columns and then to one, because a
  // two-column card in a 320px pane is not a card, it is a sliver.

  import { dismissable } from '../lib/dismiss'
  import { specOf, SPAN } from '../lib/dashboard'
  import { friendlyDate } from '../lib/format'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { overview } from '../lib/overview.svelte'
  import { panels } from '../lib/panels.svelte'
  import { purpose } from '../lib/purpose.svelte'
  import { todayIso } from '../lib/time'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import LogReading from './LogReading.svelte'
  import OverviewWidget from './OverviewWidget.svelte'
  import WidgetCard from './WidgetCard.svelte'

  // Loaded when the view appears rather than when the store is imported, so
  // a vault whose owner never opens this app never pays for the queries.
  // `start` is idempotent, so coming back refreshes instead of blanking.
  void overview.start()

  let logging = $state(false)
  /** The card being dragged, and the one the pointer is over. */
  let dragging = $state<string | null>(null)
  let over = $state<string | null>(null)

  // A tray action can ask for the reading field before this view exists to
  // give it.
  $effect(() => {
    if (overview.wantsLog) {
      overview.wantsLog = false
      logging = true
    }
  })

  const today = todayIso()
  /** Does anything on the page care which week is showing? */
  const weekly = $derived(overview.widgets.some((w) => specOf(w.type).needs.includes('balance')))

  const weekLabel = $derived(
    overview.thisWeek
      ? 'This week'
      : `${friendlyDate(overview.weekStart)} – ${friendlyDate(overview.weekEnd)}`,
  )

  /** The page itself, where there is no card under the pointer. */
  function pageMenu(): MenuItem[] {
    return tidyMenu([
      {
        label: overview.editing ? 'Stop arranging' : 'Arrange this page',
        icon: 'grip',
        run: () => (overview.editing = !overview.editing),
      },
      { label: 'Record a reading…', icon: 'plus', run: () => (logging = true) },
      SEP,
      { label: 'Refresh', icon: 'refresh', run: () => void overview.refresh() },
      {
        label: 'Roles and goals…',
        icon: 'compass',
        hint: 'in Settings',
        run: () => panels.openSettings('profile'),
      },
      SEP,
      {
        label: 'Put the page back as it was',
        icon: 'refresh',
        danger: true,
        run: () => overview.restoreDefaults(),
      },
    ])
  }

  function onDragStart(e: DragEvent, id: string) {
    if (!overview.editing) return
    dragging = id
    e.dataTransfer?.setData('text/plain', id)
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move'
  }

  function onDragOver(e: DragEvent, id: string) {
    if (!dragging || dragging === id) return
    e.preventDefault()
    over = id
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move'
  }

  function onDrop(id: string | null) {
    if (dragging) overview.reorder(dragging, id)
    dragging = null
    over = null
  }
</script>

<div class="overview">
  <header class="bar">
    <div class="titles">
      <h1>Overview</h1>
      <span class="sub">
        {#if weekly}{weekLabel}{/if}
        {#if overview.loading}· loading…{/if}
      </span>
    </div>

    <div class="tools">
      <div class="logwrap">
        <button
          class="btn"
          aria-haspopup="dialog"
          aria-expanded={logging}
          onclick={() => (logging = !logging)}
        >
          <Icon name="plus" size={14} /> Record something
        </button>
        {#if logging}
          <div
            class="logpop"
            role="dialog"
            aria-label="Record a reading"
            use:dismissable={{ onaway: () => (logging = false), within: '.logwrap .btn' }}
          >
            <!-- No journal and no entry: this was ticked on no page at all,
                 which is exactly what a nullable journal pointer on a reading
                 is for. -->
            <LogReading date={today} onclose={() => (logging = false)} />
          </div>
        {/if}
      </div>

      {#if weekly}
        <div class="week-nav">
          <button aria-label="The week before" onclick={() => overview.goWeek(-1)}>
            <span class="prev"><Icon name="chevron" size={14} /></span>
          </button>
          <button onclick={() => overview.goWeek(0)} disabled={overview.thisWeek}>Today</button>
          <button aria-label="The week after" onclick={() => overview.goWeek(1)}>
            <span class="next"><Icon name="chevron" size={14} /></span>
          </button>
        </div>
      {/if}

      <button
        class="btn"
        class:on={overview.editing}
        aria-pressed={overview.editing}
        onclick={() => (overview.editing = !overview.editing)}
      >
        <Icon name="grip" size={14} />
        {overview.editing ? 'Done' : 'Arrange'}
      </button>
    </div>
  </header>

  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="scroll body"
    oncontextmenu={(e) => menu.show(e, pageMenu())}
    ondragover={(e) => {
      if (dragging) e.preventDefault()
    }}
    ondrop={() => onDrop(null)}
  >
    {#if overview.widgets.length === 0}
      <EmptyState lead="This page is empty, which is a thing you are allowed to do.">
        {#snippet icon()}<Icon name="compass" size={28} />{/snippet}
        {#snippet note()}
          Everything the Overview can show is in the panel on the left, filed under what it is
          about. Add the two or three you would actually look at.
        {/snippet}
        {#snippet action()}
          <button class="btn btn-primary" onclick={() => overview.restoreDefaults()}>
            Start me off with a few
          </button>
        {/snippet}
      </EmptyState>
    {:else}
      <div class="grid">
        {#each overview.widgets as w, i (w.id)}
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div
            class="slot"
            style="--span: {SPAN[w.size]}"
            draggable={overview.editing}
            ondragstart={(e) => onDragStart(e, w.id)}
            ondragend={() => {
              dragging = null
              over = null
            }}
            ondragover={(e) => onDragOver(e, w.id)}
            ondragleave={() => {
              if (over === w.id) over = null
            }}
            ondrop={(e) => {
              e.stopPropagation()
              onDrop(w.id)
            }}
          >
            <WidgetCard
              widget={w}
              index={i}
              count={overview.widgets.length}
              dragOver={over === w.id}
            >
              <OverviewWidget widget={w} />
            </WidgetCard>
          </div>
        {/each}
      </div>

      {#if purpose.roles.length === 0 && overview.needs.has('purpose')}
        <!-- Roles are never seeded unasked: a list of what a life is made of
             is a claim, and writing one for somebody would be this
             application telling them who they are. Said once, under the
             cards that cannot fill themselves without it. -->
        <p class="hint">
          Some of these are about <em>roles</em> — who you are being — and nothing has said what
          yours are.
          <button class="link" onclick={() => panels.openSettings('profile')}>
            Set them up in About You
          </button>
        </p>
      {/if}
    {/if}
  </div>
</div>

<style>
  .overview {
    display: flex;
    flex: 1;
    min-width: 0;
    flex-direction: column;
  }

  .bar {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    flex: none;
    min-height: var(--header-h);
    padding: 0 var(--sp-4);
    border-bottom: 1px solid var(--border);
  }

  .titles {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    flex: 1;
    min-width: 0;
  }

  h1 {
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 600;
  }

  .sub {
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  .tools {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }

  .btn.on {
    background: var(--bg-active);
    color: var(--fg);
  }

  .body {
    flex: 1;
    padding: var(--sp-4) var(--sp-4) var(--fab-clear);
  }

  /* Six columns, so a two-, three- or six-wide card always tiles. The
     `minmax(0, 1fr)` matters: a plain `1fr` is `minmax(auto, 1fr)`, and a
     chart with a long label in it would then push its column wider than its
     share and knock the row out of alignment. */
  .grid {
    display: grid;
    grid-template-columns: repeat(6, minmax(0, 1fr));
    gap: var(--sp-3);
    align-items: start;
  }

  .slot {
    grid-column: span var(--span);
    min-width: 0;
  }

  /* A narrow window is a single column of cards rather than six slivers.
     Two steps, because three-across is still readable at tablet widths and
     dropping straight to one wastes a lot of a 900px pane. */
  @media (max-width: 1180px) {
    .grid {
      grid-template-columns: repeat(3, minmax(0, 1fr));
    }
    .slot {
      grid-column: span min(var(--span), 3);
    }
  }
  @media (max-width: 720px) {
    .grid {
      grid-template-columns: minmax(0, 1fr);
    }
    .slot {
      grid-column: span 1;
    }
  }

  .logwrap {
    position: relative;
  }

  .logpop {
    position: absolute;
    top: calc(100% + var(--sp-2));
    right: 0;
    z-index: 20;
    width: min(420px, 80vw);
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-raised);
    box-shadow: var(--shadow-lg);
  }

  .week-nav {
    display: flex;
    gap: 2px;
  }
  .week-nav button {
    display: grid;
    place-items: center;
    height: 28px;
    min-width: 28px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  .week-nav button:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .week-nav button:disabled {
    opacity: 0.4;
  }
  .prev {
    display: grid;
    place-items: center;
    rotate: 180deg;
  }
  .next {
    display: grid;
    place-items: center;
  }

  .hint {
    margin: var(--sp-4) 0 0;
    color: var(--fg-subtle);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }

  .link {
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .link:hover {
    text-decoration: underline;
  }
</style>
