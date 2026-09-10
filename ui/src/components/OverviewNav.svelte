<script lang="ts">
  // The Overview's half of the sidebar: everything the page could show,
  // filed under what it is about.
  //
  // A catalogue rather than a list of what is *on* the page. The page itself
  // is the list of what is on the page, three inches to the right, and a
  // second copy of it here would be a thing to keep in step for no reason.
  // What the sidebar is for is the other direction: finding the card you did
  // not know existed.
  //
  // The roles that used to live here have gone to Settings, under About You.
  // They were here on the argument that a thing is defined where its data is
  // seen -- the same argument that keeps a library shelf in the library's
  // sidebar -- and that argument turned out to be wrong for this one. A role
  // is not the Overview's data: it is a fact about the person that the todo
  // app, the calendar, the journal and the shelf all file things under, and
  // the Overview happened to be the app that drew it first.

  import { GROUPS, WIDGETS, WIDGET_TYPES, type Group, type WidgetType } from '../lib/dashboard'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { overview } from '../lib/overview.svelte'
  import { panels } from '../lib/panels.svelte'
  import Icon from './Icon.svelte'

  /** How many of each type are already on the page, so a row can say so. */
  const counts = $derived.by(() => {
    const out = new Map<WidgetType, number>()
    for (const w of overview.widgets) out.set(w.type, (out.get(w.type) ?? 0) + 1)
    return out
  })

  const byGroup = $derived(
    GROUPS.map((group: Group) => ({
      group,
      types: WIDGET_TYPES.filter((t) => WIDGETS[t].group === group),
    })).filter((g) => g.types.length > 0),
  )

  function navMenu(): MenuItem[] {
    return tidyMenu([
      {
        label: overview.editing ? 'Stop arranging' : 'Arrange this page',
        icon: 'grip',
        run: () => (overview.editing = !overview.editing),
      },
      SEP,
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
</script>

<nav class="scroll nav" oncontextmenu={(e) => menu.show(e, navMenu())}>
  <p class="lead">
    Your page, made of these. Add what you would actually look at; take off what you would not.
  </p>

  {#each byGroup as section (section.group)}
    <div class="head"><span class="eyebrow">{section.group}</span></div>
    {#each section.types as type (type)}
      {@const spec = WIDGETS[type]}
      {@const on = counts.get(type) ?? 0}
      <button
        class="row"
        title={spec.note}
        onclick={() => overview.add(type)}
        aria-label="Add {spec.label} to the page"
      >
        <span class="icon"><Icon name={spec.icon} size={15} /></span>
        <span class="text">
          <span class="name">{spec.label}</span>
          <span class="note">{spec.note}</span>
        </span>
        {#if on > 0}
          <!-- How many are already up there. A widget can be added twice on
               purpose -- two heatmaps of two habits is the ordinary case --
               so this is a count rather than a tick that disables the row. -->
          <span class="count">{on}</span>
        {/if}
        <span class="plus"><Icon name="plus" size={14} /></span>
      </button>
    {/each}
  {/each}

  <div class="foot">
    <button class="quietlink" onclick={() => overview.restoreDefaults()}>
      Put the page back as it was
    </button>
  </div>
</nav>

<style>
  .nav {
    flex: 1;
    padding: var(--sp-2) var(--sp-2) var(--sp-4);
  }

  .lead {
    margin: 0;
    padding: var(--sp-2);
    color: var(--fg-faint);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }

  .head {
    display: flex;
    align-items: center;
    padding: var(--sp-5) var(--sp-2) var(--sp-2);
  }

  .row {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    width: 100%;
    padding: var(--sp-2);
    border-radius: var(--radius-sm);
    color: var(--fg-muted);
    text-align: left;
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }

  .row:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .icon {
    display: grid;
    flex: none;
    place-items: center;
    width: 16px;
    height: 18px;
    color: var(--fg-faint);
  }

  .text {
    display: flex;
    flex: 1;
    min-width: 0;
    flex-direction: column;
    gap: 1px;
  }

  .name {
    font-size: var(--text-base);
    color: inherit;
  }

  /* The line that makes this a catalogue rather than a menu: it says what
     the card answers, which is the only way to choose between fifteen of
     them without adding each one to find out. */
  .note {
    color: var(--fg-faint);
    font-size: var(--text-xs);
    line-height: var(--leading-snug);
  }

  .count {
    flex: none;
    margin-top: 2px;
    padding: 0 5px;
    border-radius: 999px;
    background: var(--bg-active);
    color: var(--fg-muted);
    font-size: var(--text-xs);
    font-weight: 650;
    font-variant-numeric: tabular-nums;
  }

  .plus {
    display: grid;
    flex: none;
    place-items: center;
    width: 18px;
    height: 18px;
    color: var(--fg-faint);
    opacity: 0;
    transition: opacity var(--fast) var(--ease);
  }
  .row:hover .plus,
  .row:focus-visible .plus {
    opacity: 1;
  }

  .foot {
    margin-top: var(--sp-6);
    padding: var(--sp-3) var(--sp-2) 0;
    border-top: 1px solid var(--border);
  }

  .quietlink {
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }
  .quietlink:hover {
    color: var(--fg);
    text-decoration: underline;
  }
</style>
