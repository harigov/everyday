<script lang="ts">
  import { untrack } from 'svelte'
  import { app } from '../lib/state.svelte'
  import { sidebar } from '../lib/sidebar.svelte'
  import { phone } from '../lib/phone.svelte'
  import { DEFAULT_COLORS } from '../lib/colors'
  import { focusOnMount } from '../lib/focus'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { colourItems } from '../lib/menus'
  import Icon from './Icon.svelte'
  import Symbol from './Symbol.svelte'
  import JournalSettings from './JournalSettings.svelte'
  import TodoNav from './TodoNav.svelte'
  import CalendarNav from './CalendarNav.svelte'
  import LibraryNav from './LibraryNav.svelte'
  import MailNav from './MailNav.svelte'
  import FeedNav from './FeedNav.svelte'
  import MoneyNav from './MoneyNav.svelte'
  import ScreenTimeNav from './ScreenTimeNav.svelte'
  import FilesNav from './FilesNav.svelte'
  import HealthNav from './HealthNav.svelte'
  import AssistantNav from './AssistantNav.svelte'
  import NotesNav from './NotesNav.svelte'
  import PaneResizer from './PaneResizer.svelte'
  import type { Journal } from '../lib/types'

  let {
    folded = false,
  }: {
    /**
     * Folded away by the button in the strip above -- see `sidebar.svelte.ts`.
     * Still mounted, and drawn over the app while `sidebar.peeking`, but
     * otherwise hidden and inert: out of the tab order and out of reach of a
     * screen reader, which is what "not on screen" has to mean for both.
     */
    folded?: boolean
  } = $props()

  const peeking = $derived(folded && sidebar.peeking)

  // The menu's hold ends when the menu does -- chosen from, or dismissed.
  $effect(() => {
    if (!menu.at) untrack(() => sidebar.letGo('menu'))
  })

  // The apps themselves are `AppBar`, outside this: they are not one app's
  // navigation, and everything below is.
  let creating = $state(false)
  let draft = $state('')

  async function create() {
    const name = draft.trim()
    draft = ''
    creating = false
    if (!name) return
    // The id, timestamps and defaults come from the core. Minting them here
    // meant depending on `crypto.randomUUID`, which needs a secure context
    // the packaged webview does not always provide -- and when it is missing
    // the journal is silently never created.
    await app.newJournal(
      name,
      DEFAULT_COLORS[app.journals.length % DEFAULT_COLORS.length]!,
      app.journals.length,
    )
  }

  // Which journal's settings are open. A journal used to have none: it was
  // named once at creation, and right-clicking it deleted it -- with a
  // confirmation as the only sign that a right-click was destructive at all.
  // Now that a journal also carries what it tracks there is somewhere for
  // both to live, and deleting is a button inside it rather than a gesture
  // one slip away from a year of entries.
  let settingsFor = $state<Journal | null>(null)

  /** Re-read from the store, so a save inside the dialog is reflected. */
  const settingsJournal = $derived(
    settingsFor ? (app.journals.find((j) => j.id === settingsFor!.id) ?? null) : null,
  )

  /**
   * What a right-click on a journal offers.
   *
   * Note what is *not* in it: delete. That moved inside the settings dialog
   * on purpose -- a journal is a year of entries and it should not be one
   * slip from a menu away -- and putting it back here would undo the point
   * of moving it. Naming and the rest of the journal live in there too, so
   * this offers the two things you would otherwise cross the window for, the
   * colour, and the way in.
   */
  function journalMenu(j: Journal): MenuItem[] {
    const only = app.selectedJournal === j.id && !app.showStarredOnly
    return tidyMenu([
      {
        label: 'New entry here',
        icon: 'plus',
        run: async () => {
          app.showStarredOnly = false
          await app.selectJournal(j.id)
          await app.newEntry()
        },
      },
      {
        label: only ? 'Show every journal' : 'Show only this journal',
        icon: 'layers',
        run: () => {
          app.showStarredOnly = false
          void app.selectJournal(only ? null : j.id)
        },
      },
      SEP,
      {
        label: 'Colour',
        dot: j.color,
        items: colourItems(j.color, (color) => app.saveJournal({ ...$state.snapshot(j), color })),
      },
      { label: 'Journal settings…', icon: 'settings', run: () => (settingsFor = j) },
    ])
  }

  // Theirs: "All entries" leaves out the assistant's own journal.
  const total = $derived(
    (app.status?.stats?.entries ?? 0) - (app.status?.stats?.assistantEntries ?? 0),
  )
</script>

<!-- While folded, the pointer and the keyboard are what keep a peek open:
     arriving holds it, leaving lets it go after a moment -- see
     `sidebar.svelte.ts` for the delays. Escape puts it away at once. -->
<aside
  class="sidebar shell-card"
  class:folded
  class:frost={folded}
  class:peeking
  inert={folded && !peeking}
  aria-hidden={folded && !peeking ? 'true' : undefined}
  onpointerenter={(e) => {
    if (folded && e.pointerType === 'mouse') sidebar.reach()
  }}
  onpointerleave={(e) => {
    if (folded && e.pointerType === 'mouse') sidebar.release()
  }}
  onclick={(e) => {
    // A phone's drawer goes once somewhere is chosen in it. See
    // `sidebar.open`.
    if (phone.on && e.target instanceof Element && e.target.closest('.side-row')) sidebar.hide()
  }}
  onfocusin={(e) => {
    // The keyboard gets it at once -- the hover delay is there for a pointer
    // passing by, and focus arriving is never that -- and keeps it while it
    // is here, as does a field being typed into: neither must go inert
    // because the pointer drifted off the panel. A row a mouse click
    // focused does not hold it, or a quick choice of folder would leave the
    // sidebar out over the app until somebody clicked somewhere else.
    if (!folded) return
    const at = e.target
    const typing =
      at instanceof HTMLElement &&
      (at.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(at.tagName))
    if (typing || (at instanceof Element && at.matches(':focus-visible'))) sidebar.hold('focus')
  }}
  onfocusout={(e) => {
    const to = e.relatedTarget
    if (folded && !(to instanceof Node && e.currentTarget.contains(to))) sidebar.letGo('focus')
  }}
  oncontextmenucapture={() => {
    // A row's menu is drawn outside this panel, so the pointer leaves it to
    // reach the menu; the menu holds the peek until it closes, so "Rename"
    // does not open its field inside a sidebar that has just gone. Captured,
    // because the row stops the event before it would bubble here.
    if (!folded) return
    sidebar.hold('menu')
    // A row with nothing to offer opens no menu, and must not hold forever.
    // Asked on the next task, not in a microtask: the row's own handler is
    // delegated to the document root, and the browser drains microtasks
    // between listeners -- so a microtask here runs before the menu exists.
    setTimeout(() => {
      if (!menu.at) sidebar.letGo('menu')
    }, 0)
  }}
  onkeydown={(e) => {
    if (folded && e.key === 'Escape') sidebar.hide()
  }}
>
  {#if app.section === 'assistant'}
    <AssistantNav />
  {:else if app.section === 'notes'}
    <NotesNav />
  {:else if app.section === 'todo'}
    <TodoNav />
  {:else if app.section === 'calendar'}
    <CalendarNav />
  {:else if app.section === 'library'}
    <LibraryNav />
  {:else if app.section === 'mail'}
    <MailNav />
  {:else if app.section === 'feed'}
    <FeedNav />
  {:else if app.section === 'money'}
    <MoneyNav />
  {:else if app.section === 'screentime'}
    <ScreenTimeNav />
  {:else if app.section === 'files'}
    <FilesNav />
  {:else if app.section === 'health'}
    <HealthNav />
  {:else}
    <nav class="scroll side-nav">
      <button
        class="side-row"
        class:sel={app.selectedJournal === null && !app.showStarredOnly}
        onclick={() => {
          app.showStarredOnly = false
          void app.selectJournal(null)
        }}
      >
        <span class="side-icon"><Symbol name="layers" size={17} /></span>
        <span class="side-text">All entries</span>
        <span class="side-count">{total}</span>
      </button>

      <button
        class="side-row"
        class:sel={app.showStarredOnly}
        onclick={() => {
          app.showStarredOnly = true
          void app.selectJournal(null)
        }}
      >
        <span class="side-icon star"><Symbol name="star" size={16} /></span>
        <span class="side-text">Starred</span>
      </button>

      <div class="side-head">
        <span class="eyebrow">Journals</span>
        <button
          class="side-add"
          title="New journal"
          aria-label="New journal"
          onclick={() => (creating = true)}
        >
          <Icon name="plus" size={15} />
        </button>
      </div>

      {#snippet journalRow(j: Journal)}
        <div class="slot">
          <button
            class="side-row"
            class:sel={app.selectedJournal === j.id && !app.showStarredOnly}
            onclick={() => {
              app.showStarredOnly = false
              void app.selectJournal(j.id)
            }}
            oncontextmenu={(e) => menu.show(e, journalMenu(j))}
            title={j.description || j.name}
          >
            <span class="side-icon">{j.icon}</span>
            <span class="side-text">{j.name}</span>
            <span class="side-dot" style:--dot={j.color} aria-hidden="true"></span>
          </button>
          <button
            class="cog"
            title="{j.name} settings"
            aria-label="{j.name} settings"
            onclick={() => (settingsFor = j)}
          >
            <Icon name="settings" size={13} />
          </button>
        </div>
      {/snippet}

      {#each app.ownJournals as j (j.id)}
        {@render journalRow(j)}
      {/each}

      {#if creating}
        <input
          class="side-new"
          placeholder="Journal name"
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

      <!-- The assistant's own journal: notes it leaves itself for the day
           they are about. Theirs to read, apart from where they write. -->
      {#each app.journals.filter((j) => j.assistant) as j (j.id)}
        <div class="side-head">
          <span class="eyebrow">Kept by the assistant</span>
        </div>
        {@render journalRow(j)}
      {/each}
    </nav>
  {/if}
  <PaneResizer
    cssVar="--sidebar-w"
    storageKey="pane-w:sidebar"
    defaultWidth={248}
    min={180}
    max={420}
  />
</aside>

{#if settingsJournal}
  <JournalSettings journal={settingsJournal} onclose={() => (settingsFor = null)} />
{/if}

<style>
  .sidebar {
    /* As tall as the row by a stated height, not stretched: see
       `.shell-stage` in app.css. */
    height: 100%;
    position: relative;
    width: var(--sidebar-w);
    flex: none;
    display: flex;
    flex-direction: column;
    padding-top: var(--sp-1);
    background: var(--sidebar);
    border-right: var(--shell-rule);
  }

  /* Folded: out of the row, so the app takes its width, and laid over the
     app's left edge instead -- invisible until a peek draws it. Over, not
     beside: a peek that reflowed the app would move the very list somebody
     was reading. `.panes` is what it is positioned in, so it is placed by
     the same gaps `.panes` leaves around a sidebar that is in the row.

     Frosted (`.frost`): over the app rather than over the backdrop, a glass
     theme's translucent sidebar would otherwise let the list underneath read
     through it. Its own fill over the floating one: in a flat theme that is
     the same sidebar it is in the row, and in a glass theme its tint, on a
     base solid enough that the list stays out of it where the webview draws
     no blur. */
  .sidebar.folded {
    position: absolute;
    top: 0;
    bottom: var(--shell-gap);
    /* Its insets decide its height, not the row's it states in the row. */
    height: auto;
    left: calc(var(--bar-w, var(--appbar-w)) + var(--shell-gap));
    z-index: 40;
    background: linear-gradient(var(--sidebar), var(--sidebar)) var(--float);
    visibility: hidden;
    opacity: 0;
    translate: -10px 0;
    border-right-color: var(--border-strong, var(--border));
    box-shadow: var(--shadow-lg);
    transition:
      opacity var(--fast) var(--ease),
      translate var(--fast) var(--ease),
      visibility 0s linear var(--fast);
  }
  .sidebar.folded.peeking {
    visibility: visible;
    opacity: 1;
    translate: 0 0;
    transition:
      opacity var(--fast) var(--ease),
      translate var(--fast) var(--ease),
      visibility 0s;
  }
  /* A phone's drawer: the whole height of the screen, from its left edge,
     over the top bar and the bottom bar, sliding in rather than fading.
     `App.svelte` draws the scrim behind it. */
  :global([data-phone]) .sidebar.folded {
    position: fixed;
    top: 0;
    bottom: 0;
    left: 0;
    z-index: 64;
    width: var(--sidebar-w);
    padding-top: calc(var(--sp-2) + var(--safe-top));
    padding-bottom: var(--safe-bottom);
    padding-left: var(--safe-left);
    border-radius: 0 var(--radius-lg) var(--radius-lg) 0;
    opacity: 1;
    translate: -100% 0;
    transition:
      translate var(--med) var(--ease),
      visibility 0s linear var(--med);
  }
  /* `none`, not `0 0`, once out: any translate makes the drawer the box a
     `position: fixed` dialog inside it -- Add a calendar, Unfollow -- is
     placed in, and the dialog then sits in the drawer, not on the screen. */
  :global([data-phone]) .sidebar.folded.peeking {
    translate: none;
    transition:
      translate var(--med) var(--ease),
      visibility 0s;
  }
  @media (prefers-reduced-motion: reduce) {
    .sidebar.folded,
    .sidebar.folded.peeking {
      translate: 0 0;
      transition: none;
    }
  }

  /* The journal's rows, headings and counts are the shared sidebar's -- see
     app.css's "Sidebars". What is the journal's own: the starred row's star,
     and the settings button laid over each journal's row. */

  /* The settings button rides on top of the row rather than inside it: a
     row is a button, and a button inside a button is not a thing HTML has.
     Round and the size of the heading's add button, and centred exactly
     where the row's dot is, which it covers while the row is hovered. */
  .slot {
    position: relative;
  }
  .cog {
    position: absolute;
    top: 50%;
    right: var(--sp-1);
    transform: translateY(-50%);
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    color: var(--fg-faint);
    background: var(--bg-active);
    opacity: 0;
    transition: opacity var(--fast) var(--ease);
  }
  .slot:hover .cog,
  .cog:focus-visible {
    opacity: 1;
  }
  .cog:hover {
    color: var(--fg);
  }

  .star {
    color: var(--star);
  }
</style>
