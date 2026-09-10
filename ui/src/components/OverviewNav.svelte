<script lang="ts">
  // The Overview's half of the sidebar: the four panes, then the roles.
  //
  // The roles are here rather than in Settings for the reason a library
  // Kind is in the library's sidebar: a thing is defined where its data is
  // seen, and a role you have to go to Settings for is a role you will not
  // edit. Everything a role can have done to it is on its right-click menu.

  import { focusOnMount } from '../lib/focus'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { colourItems } from '../lib/menus'
  import { overview, PANES, PANE_LABELS, type Pane } from '../lib/overview.svelte'
  import { purpose } from '../lib/purpose.svelte'
  import type { IconName } from '../lib/icons'
  import type { RoleInfo } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'

  let creating = $state(false)
  let draft = $state('')
  let renaming = $state<RoleInfo | null>(null)
  let renameDraft = $state('')
  let pendingDelete = $state<RoleInfo | null>(null)

  const PANE_ICONS: Record<Pane, IconName> = {
    today: 'sun',
    week: 'week',
    goals: 'target',
    habits: 'refresh',
  }

  const roles = $derived(purpose.roles)
  const live = $derived(roles.filter((r) => !r.archived))
  const archived = $derived(roles.filter((r) => r.archived))

  async function create() {
    const name = draft.trim()
    draft = ''
    creating = false
    if (name) await purpose.addRole(name)
  }

  async function commitRename() {
    const role = renaming
    const name = renameDraft.trim()
    renaming = null
    if (role && name && name !== role.name) {
      await purpose.saveRole({ ...$state.snapshot(role), name })
    }
  }

  /**
   * What a right-click on a role offers.
   *
   * Archiving is above deleting and reads as the ordinary answer, because it
   * is: a role you have stopped playing keeps a year of attributed hours,
   * and deleting is refused while any goal still points at it anyway.
   */
  function roleMenu(role: RoleInfo): MenuItem[] {
    return tidyMenu([
      {
        label: 'Add a goal here',
        icon: 'plus',
        run: () => {
          overview.setPane('goals')
          // The composer belongs to the goals pane, which is not an ancestor
          // of this one. Reaching it by selector is what the todo app's
          // sidebar already does for its capture line.
          setTimeout(
            () => document.querySelector<HTMLInputElement>(`[data-newgoal="${role.id}"]`)?.focus(),
            0,
          )
        },
      },
      SEP,
      {
        label: 'Rename…',
        icon: 'pencil',
        run: () => {
          renameDraft = role.name
          renaming = role
        },
      },
      {
        label: 'Colour',
        dot: role.color,
        items: colourItems(role.color, (color) =>
          purpose.saveRole({ ...$state.snapshot(role), color }),
        ),
      },
      {
        label: role.archived ? 'Bring it back' : 'Archive',
        icon: role.archived ? 'refresh' : 'hidden',
        hint: role.archived ? undefined : 'keeps its history',
        run: () => purpose.saveRole({ ...$state.snapshot(role), archived: !role.archived }),
      },
      SEP,
      {
        label: 'Delete role…',
        icon: 'trash',
        danger: true,
        // Said here rather than discovered on the way: a role with goals
        // under it cannot be deleted, and offering the row as though it
        // could is a dialog that ends in a refusal.
        disabled: role.goals > 0,
        hint: role.goals > 0 ? 'move its goals first' : undefined,
        run: () => (pendingDelete = role),
      },
    ])
  }

  function navMenu(): MenuItem[] {
    return tidyMenu([
      { label: 'New role', icon: 'plus', run: () => (creating = true) },
      SEP,
      ...PANES.map((p) => ({
        label: PANE_LABELS[p],
        icon: PANE_ICONS[p],
        checked: overview.pane === p,
        run: () => overview.setPane(p),
      })),
    ])
  }
</script>

<nav class="scroll nav" oncontextmenu={(e) => menu.show(e, navMenu())}>
  {#each PANES as pane (pane)}
    <button class="row" class:sel={overview.pane === pane} onclick={() => overview.setPane(pane)}>
      <span class="icon"><Icon name={PANE_ICONS[pane]} /></span>
      <span class="text">{PANE_LABELS[pane]}</span>
    </button>
  {/each}

  <div class="head">
    <span class="eyebrow">Roles</span>
    <button class="plus" title="New role" aria-label="New role" onclick={() => (creating = true)}>
      <Icon name="plus" size={15} />
    </button>
  </div>

  {#each live as role (role.id)}
    {#if renaming?.id === role.id}
      <input
        class="new"
        bind:value={renameDraft}
        use:focusOnMount
        onblur={commitRename}
        onkeydown={(e) => {
          if (e.key === 'Enter') void commitRename()
          if (e.key === 'Escape') renaming = null
        }}
      />
    {:else}
      <button
        class="row"
        style="--dot: {role.color}"
        onclick={() => overview.setPane('goals')}
        oncontextmenu={(e) => menu.show(e, roleMenu(role))}
        title="{role.name} — {role.goals} {role.goals === 1 ? 'goal' : 'goals'}"
      >
        <span class="icon">{role.icon}</span>
        <span class="text">{role.name}</span>
        <!-- What is still being pursued, not the total. A role with forty
             finished goals is not forty things to think about. -->
        {#if role.open > 0}<span class="count">{role.open}</span>{/if}
        <span class="dot" aria-hidden="true"></span>
      </button>
    {/if}
  {/each}

  {#if creating}
    <input
      class="new"
      placeholder="Role name"
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
  {/if}

  {#if live.length === 0 && !creating}
    <p class="blank">
      A role is who you are being — <em>Parent</em>, <em>Work</em>, <em>Myself</em>. Add a few and
      the week below starts adding up.
    </p>
  {/if}

  {#if archived.length > 0}
    <div class="head">
      <span class="eyebrow">Archived</span>
    </div>
    {#each archived as role (role.id)}
      <button
        class="row muted"
        style="--dot: {role.color}"
        oncontextmenu={(e) => menu.show(e, roleMenu(role))}
        onclick={() => menu.show(new MouseEvent('contextmenu'), roleMenu(role))}
      >
        <span class="icon">{role.icon}</span>
        <span class="text">{role.name}</span>
      </button>
    {/each}
  {/if}
</nav>

{#if pendingDelete}
  <ConfirmDialog
    title={'Delete the “' + pendingDelete.name + '” role?'}
    detail="Nothing that was filed under it is deleted. Time already recorded against it stops being counted under any role. This cannot be undone."
    confirmLabel="Delete role"
    onconfirm={() => {
      const role = pendingDelete
      pendingDelete = null
      if (role) void purpose.deleteRole(role.id)
    }}
    oncancel={() => (pendingDelete = null)}
  />
{/if}

<style>
  .nav {
    flex: 1;
    padding: var(--sp-2) var(--sp-2) var(--sp-4);
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--sp-5) var(--sp-2) var(--sp-2);
  }

  .plus {
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }

  .plus:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    height: var(--row-h);
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    color: var(--fg-muted);
    font-size: var(--text-base);
    text-align: left;
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }

  .row:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .row.sel {
    background: var(--bg-active);
    color: var(--fg);
    font-weight: 550;
  }

  .row.muted {
    opacity: 0.55;
  }

  .icon {
    display: grid;
    flex: none;
    place-items: center;
    width: 16px;
    height: 16px;
    font-size: var(--text-sm);
    line-height: 1;
  }

  .text {
    flex: 1 1 auto;
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

  .dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--dot);
  }

  .new {
    width: 100%;
    height: var(--row-h);
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-base);
  }

  .blank {
    margin: 0;
    padding: var(--sp-2);
    color: var(--fg-faint);
    font-size: var(--text-sm);
    line-height: var(--leading);
  }
</style>
