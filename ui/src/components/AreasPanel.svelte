<script lang="ts">
  // The parts of a life this vault is organised around, edited in Settings.
  //
  // Areas used to be defined in the Overview's sidebar, on the rule this
  // application follows almost everywhere: a thing is defined where its data
  // is seen, the way a library shelf is made in the library's sidebar. That
  // rule was applied to the wrong noun here. A shelf belongs to the library
  // and nothing else has an opinion about it; an area is filed against by the
  // todo app, the calendar, the journal and the shelf alike, and the Overview
  // was only the app that happened to draw it first. It is a standing fact
  // about the person, which is what this tab is — and which is why it sits
  // under their name, their birthday and what they do all day rather than
  // inside a report.
  //
  // Archiving is above deleting and reads as the ordinary answer, because it
  // is: an area you have stopped playing keeps a year of attributed hours, and
  // deleting is refused by the backend while any goal still points at it.

  import { focusOnMount } from '../lib/focus'
  import { plural } from '../lib/format'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { colourItems } from '../lib/menus'
  import { panels } from '../lib/panels.svelte'
  import { purpose } from '../lib/purpose.svelte'
  import { app } from '../lib/state.svelte'
  import { todo } from '../lib/todo.svelte'
  import type { AreaInfo } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'

  let creating = $state(false)
  let draft = $state('')
  let renaming = $state<AreaInfo | null>(null)
  let renameDraft = $state('')
  let pendingDelete = $state<AreaInfo | null>(null)

  void purpose.load()

  const live = $derived(purpose.areas.filter((r) => !r.archived))
  const archived = $derived(purpose.areas.filter((r) => r.archived))
  const writable = $derived(app.status?.writable !== false)

  async function create() {
    const name = draft.trim()
    draft = ''
    creating = false
    if (name) await purpose.addArea(name)
  }

  async function commitRename() {
    const area = renaming
    const name = renameDraft.trim()
    renaming = null
    if (area && name && name !== area.name) {
      await purpose.saveArea({ ...$state.snapshot(area), name })
    }
  }

  function areaMenu(area: AreaInfo): MenuItem[] {
    return tidyMenu([
      {
        label: 'Rename…',
        icon: 'pencil',
        run: () => {
          renameDraft = area.name
          renaming = area
        },
      },
      {
        label: 'Colour',
        dot: area.color,
        items: colourItems(area.color, (color) =>
          purpose.saveArea({ ...$state.snapshot(area), color }),
        ),
      },
      {
        label: area.archived ? 'Bring it back' : 'Archive',
        icon: area.archived ? 'refresh' : 'hidden',
        hint: area.archived ? undefined : 'keeps its history',
        run: () => purpose.saveArea({ ...$state.snapshot(area), archived: !area.archived }),
      },
      SEP,
      {
        label: 'Its goals…',
        icon: 'target',
        run: () => {
          panels.closeSettings()
          void app.goTo('todo').then((went) => {
            if (went) void todo.setScope({ kind: 'goals' })
          })
        },
      },
      SEP,
      {
        label: 'Delete area…',
        icon: 'trash',
        danger: true,
        // Said here rather than discovered on the way: an area with goals
        // under it cannot be deleted, and offering the row as though it
        // could is a dialog that ends in a refusal.
        disabled: area.goals > 0,
        hint: area.goals > 0 ? 'move its goals first' : undefined,
        run: () => (pendingDelete = area),
      },
    ])
  }
</script>

<section>
  <span class="eyebrow">Your areas</span>
  <p class="hint">
    The parts of your life — <em>Family</em>, <em>Work</em>, <em>Health</em>. Everything that takes
    time can be filed under one, which is what lets the Overview say where a week went, and what a
    goal in the Todo app hangs from.
  </p>

  {#if live.length === 0 && !creating}
    <p class="hint">
      None yet. Nothing writes this list for you: what a life is made of is a claim, and it is not
      one this application gets to make.
    </p>
    <div>
      <button class="btn btn-outline" disabled={!writable} onclick={() => void purpose.seed()}>
        Start me off with a few
      </button>
    </div>
  {/if}

  <ul class="areas">
    {#each live as area (area.id)}
      <li>
        {#if renaming?.id === area.id}
          <input
            class="field"
            aria-label="Area name"
            bind:value={renameDraft}
            use:focusOnMount
            onblur={commitRename}
            onkeydown={(e) => {
              if (e.key === 'Enter') void commitRename()
              if (e.key === 'Escape') renaming = null
            }}
          />
        {:else}
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div class="area" oncontextmenu={(e) => menu.show(e, areaMenu(area))}>
            <span class="dot" style="background: {area.color}"></span>
            <span class="glyph">{area.icon}</span>
            <span class="name">{area.name}</span>
            <span class="count">
              {area.open > 0 ? `${area.open} open` : plural(area.goals, 'goal')}
            </span>
            <button
              class="more"
              title="What can be done to this area"
              aria-label="More, for {area.name}"
              onclick={(e) => menu.show(e, areaMenu(area))}
            >
              <Icon name="chevron" size={13} />
            </button>
          </div>
        {/if}
      </li>
    {/each}
  </ul>

  {#if creating}
    <input
      class="field"
      placeholder="Area name"
      bind:value={draft}
      use:focusOnMount
      onblur={create}
      onkeydown={(e) => {
        if (e.key === 'Enter') void create()
        if (e.key === 'Escape') {
          draft = ''
          creating = false
        }
      }}
    />
  {:else}
    <div>
      <button class="btn btn-outline" disabled={!writable} onclick={() => (creating = true)}>
        <Icon name="plus" size={13} /> Add an area
      </button>
    </div>
  {/if}

  {#if archived.length > 0}
    <span class="eyebrow archived">Archived</span>
    <ul class="areas">
      {#each archived as area (area.id)}
        <li>
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div class="area muted" oncontextmenu={(e) => menu.show(e, areaMenu(area))}>
            <span class="dot" style="background: {area.color}"></span>
            <span class="glyph">{area.icon}</span>
            <span class="name">{area.name}</span>
            <button
              class="more"
              aria-label="More, for {area.name}"
              onclick={(e) => menu.show(e, areaMenu(area))}
            >
              <Icon name="chevron" size={13} />
            </button>
          </div>
        </li>
      {/each}
    </ul>
    <p class="hint">
      An archived area keeps every hour ever attributed to it and stops appearing in the pickers.
    </p>
  {/if}
</section>

{#if pendingDelete}
  <ConfirmDialog
    title={'Delete the “' + pendingDelete.name + '” area?'}
    detail="Nothing that was filed under it is deleted. Time already recorded against it stops being counted under any area. This cannot be undone."
    confirmLabel="Delete area"
    onconfirm={() => {
      const area = pendingDelete
      pendingDelete = null
      if (area) void purpose.deleteArea(area.id)
    }}
    oncancel={() => (pendingDelete = null)}
  />
{/if}

<style>
  section {
    display: grid;
    gap: var(--sp-2);
    padding: var(--sp-4) 0;
    border-bottom: 1px solid var(--border);
  }

  .hint {
    margin: 0;
    color: var(--fg-subtle);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }

  .areas {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .area {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: var(--row-h);
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-base);
    color: var(--fg);
  }
  .area:hover {
    background: var(--bg-hover);
  }
  .area.muted {
    opacity: 0.6;
  }

  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }

  .glyph {
    font-size: var(--text-sm);
    line-height: 1;
  }

  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .count {
    flex: none;
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  /* The same menu the right-click raises, for anybody who does not know
     there is one. A chevron rotated to point down, which is the icon set's
     one direction. */
  .more {
    display: grid;
    flex: none;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    rotate: 90deg;
  }
  .more:hover {
    background: var(--bg-active);
    color: var(--fg);
  }

  .archived {
    margin-top: var(--sp-4);
  }

  /* A phone: a row and its menu button a finger tall. */
  :global([data-phone]) .area {
    height: 48px;
    padding-right: 0;
  }
  :global([data-phone]) .more {
    width: 44px;
    height: 44px;
  }
</style>
