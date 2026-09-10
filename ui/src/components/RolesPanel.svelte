<script lang="ts">
  // The parts of a life this vault is organised around, edited in Settings.
  //
  // Roles used to be defined in the Overview's sidebar, on the rule this
  // application follows almost everywhere: a thing is defined where its data
  // is seen, the way a library shelf is made in the library's sidebar. That
  // rule was applied to the wrong noun here. A shelf belongs to the library
  // and nothing else has an opinion about it; a role is filed against by the
  // todo app, the calendar, the journal and the shelf alike, and the Overview
  // was only the app that happened to draw it first. It is a standing fact
  // about the person, which is what this tab is — and which is why it sits
  // under their name, their birthday and what they do all day rather than
  // inside a report.
  //
  // Archiving is above deleting and reads as the ordinary answer, because it
  // is: a role you have stopped playing keeps a year of attributed hours, and
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
  import type { RoleInfo } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'

  let creating = $state(false)
  let draft = $state('')
  let renaming = $state<RoleInfo | null>(null)
  let renameDraft = $state('')
  let pendingDelete = $state<RoleInfo | null>(null)

  void purpose.load()

  const live = $derived(purpose.roles.filter((r) => !r.archived))
  const archived = $derived(purpose.roles.filter((r) => r.archived))
  const writable = $derived(app.status?.writable !== false)

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

  function roleMenu(role: RoleInfo): MenuItem[] {
    return tidyMenu([
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
        label: 'Its goals…',
        icon: 'target',
        run: () => {
          panels.closeSettings()
          void app.goTo('todo').then(() => todo.setScope({ kind: 'goals' }))
        },
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
</script>

<section>
  <span class="eyebrow">Your roles</span>
  <p class="hint">
    Who you are being — <em>Parent</em>, <em>Work</em>, <em>Myself</em>. Everything that takes time
    can be filed under one, which is what lets the Overview say where a week went, and what a goal
    in the Todo app hangs from.
  </p>

  {#if live.length === 0 && !creating}
    <p class="hint">
      None yet. Nothing writes this list for you: what a life is made of is a claim, and it is not
      one this application gets to make.
    </p>
    <div>
      <button class="btn" disabled={!writable} onclick={() => void purpose.seed()}>
        Start me off with a few
      </button>
    </div>
  {/if}

  <ul class="roles">
    {#each live as role (role.id)}
      <li>
        {#if renaming?.id === role.id}
          <input
            class="field"
            aria-label="Role name"
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
          <div class="role" oncontextmenu={(e) => menu.show(e, roleMenu(role))}>
            <span class="dot" style="background: {role.color}"></span>
            <span class="glyph">{role.icon}</span>
            <span class="name">{role.name}</span>
            <span class="count">
              {role.open > 0 ? `${role.open} open` : plural(role.goals, 'goal')}
            </span>
            <button
              class="more"
              title="What can be done to this role"
              aria-label="More, for {role.name}"
              onclick={(e) => menu.show(e, roleMenu(role))}
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
  {:else}
    <div>
      <button class="btn" disabled={!writable} onclick={() => (creating = true)}>
        <Icon name="plus" size={13} /> Add a role
      </button>
    </div>
  {/if}

  {#if archived.length > 0}
    <span class="eyebrow archived">Archived</span>
    <ul class="roles">
      {#each archived as role (role.id)}
        <li>
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div class="role muted" oncontextmenu={(e) => menu.show(e, roleMenu(role))}>
            <span class="dot" style="background: {role.color}"></span>
            <span class="glyph">{role.icon}</span>
            <span class="name">{role.name}</span>
            <button
              class="more"
              aria-label="More, for {role.name}"
              onclick={(e) => menu.show(e, roleMenu(role))}
            >
              <Icon name="chevron" size={13} />
            </button>
          </div>
        </li>
      {/each}
    </ul>
    <p class="hint">
      An archived role keeps every hour ever attributed to it and stops appearing in the pickers.
    </p>
  {/if}
</section>

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
  section {
    display: grid;
    gap: var(--sp-2);
    padding: var(--sp-4) 0;
    border-bottom: 1px solid var(--line);
  }

  .hint {
    margin: 0;
    color: var(--fg-subtle);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }

  .roles {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .role {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: var(--row-h);
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-base);
    color: var(--fg);
  }
  .role:hover {
    background: var(--bg-hover);
  }
  .role.muted {
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
</style>
