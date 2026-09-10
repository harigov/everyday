<script lang="ts">
  import { api, onSaveAndClose } from './lib/api'
  // The stores, then the components, rather than the two interleaved.
  //
  // Importing a store is also what *constructs* it, and construction is what
  // registers its lock and flush hooks with `app`. So every app's store is
  // named here even when this file has nothing else to say to it -- those are
  // the side-effect imports below, and they are the reason a lock writes out a
  // half-typed note belonging to an app this component never mentions.
  //
  // App bar order first, then what every app shares, then the window's own.
  import { app, type Section } from './lib/state.svelte'
  import './lib/notes.svelte'
  import { todo } from './lib/todo.svelte'
  import './lib/calendar.svelte'
  import { library } from './lib/library.svelte'
  import { assistant } from './lib/assistant.svelte'
  import { purpose } from './lib/purpose.svelte'
  import { quick } from './lib/quick.svelte'
  import { tracking } from './lib/tracking.svelte'
  import { agent } from './lib/agent.svelte'
  import { live } from './lib/live.svelte'
  import { menu } from './lib/menu.svelte'
  import { notify } from './lib/notify.svelte'
  import { panels } from './lib/panels.svelte'
  import { shortcuts } from './lib/shortcuts.svelte'
  import { tray } from './lib/tray.svelte'
  import AppBar from './components/AppBar.svelte'
  import Sidebar from './components/Sidebar.svelte'
  import EntryList from './components/EntryList.svelte'
  import Editor from './components/Editor.svelte'
  import NotesView from './components/NotesView.svelte'
  import TodoView from './components/TodoView.svelte'
  import CalendarView from './components/CalendarView.svelte'
  import LibraryView from './components/LibraryView.svelte'
  import OverviewView from './components/OverviewView.svelte'
  import AssistantView from './components/AssistantView.svelte'
  import LockScreen from './components/LockScreen.svelte'
  import Setup from './components/Setup.svelte'
  import ErrorScreen from './components/ErrorScreen.svelte'
  import Logo from './components/Logo.svelte'
  import Notices from './components/Notices.svelte'
  import Toasts from './components/Toasts.svelte'
  import ContextMenu from './components/ContextMenu.svelte'
  import ChatPanel from './components/ChatPanel.svelte'
  import Icon from './components/Icon.svelte'
  import SettingsDialog from './components/SettingsDialog.svelte'
  import ShortcutsHelp from './components/ShortcutsHelp.svelte'
  import Palette from './components/Palette.svelte'

  void app.start()
  // Let the Rust shell speak. Its background work -- refreshing subscribed
  // calendars on a timer -- previously had nowhere to report to but the log.
  notify.listenToShell()
  // A toast can be quoting an entry title or a project name, which are
  // decrypted contents of the vault. They go when the key does, for the same
  // reason the search index does.
  app.onLock(() => notify.clear())
  // And for the same reason: an open menu is quoting the row it was raised
  // on -- an entry title, a project name -- and those are vault contents.
  app.onLock(() => menu.close(false))

  // Notice writes that happened somewhere else. On a local vault that is this
  // window's own commands and it already knows; under server mode it is another
  // machine, and this is what keeps a list from going stale under somebody's
  // cursor.
  live.start()

  // Put the quick actions in the menu bar, and keep them in step with the
  // vault from here on. What is in it is a filtered view over the one action
  // table, so there is nothing for an app to register first.
  tray.start()

  // Whether the assistant's rail was showing is remembered across launches:
  // it is a panel somebody either works with or does not, and reopening it
  // every session is as wrong as closing it every session. What it holds is
  // loaded by the panel itself -- see `ensureLoaded`, which also covers the
  // rail still being open after a lock cleared everything behind it.
  agent.restore()

  // Roles and goals, loaded once the vault is open and reloaded after every
  // unlock.
  //
  // Here rather than inside `state.svelte.ts`, which is where the unlock
  // itself happens: the purpose store reads `app`, so a store `app` also
  // read would be a cycle, and a cycle between two modules that both
  // construct singletons at import time is how a screen ends up blank with
  // one line in the console. This component already imports every store and
  // is where the app-level wiring belongs.
  //
  // Five unrelated places need these to draw a *name* — the picker, the
  // "File under" submenu on four different records, the todo list's
  // group-by, and every purpose chip — so loading on first use would show
  // up as a submenu that is empty the first time it is opened.
  $effect(() => {
    if (app.screen !== 'main') return
    void purpose.load()
    // The vault's trackers, and — on a vault written before they became
    // records — the one migration that cannot be a SQL step, since the old
    // definitions are inside a sealed journal payload no migration can read.
    void tracking.load()
    // Only the number, not the three lists behind it. The app bar draws it in
    // every app, so it must not cost a query per app; the Assistant app loads
    // the rest when it is opened.
    void assistant.refreshCount()
    // Which quick jobs are on. Every capture box in the application reads
    // this to decide whether to offer anything, so loading it on first use
    // would mean the first shelf, the first task and the first note of every
    // session silently got no suggestion.
    void quick.load()
  })

  // Each app tints the window with the accent of whatever it has selected:
  // the journal you are in, the project you are looking at, the shelf you are
  // browsing, or -- for the apps that span every one of those at once -- the
  // vault's own accent.
  //
  // A table keyed by `Section` rather than a chain of ternaries with a
  // fallback on the end. The chain had grown a clause per app and quietly
  // gave any new one the *journal's* accent, which is the one answer that is
  // wrong everywhere; this does not compile until the new app says which of
  // the two it wants.
  const ACCENTS: Record<Section, () => string> = {
    journal: () => app.accent,
    notes: () => 'var(--accent)',
    todo: () => todo.accent,
    calendar: () => 'var(--accent)',
    library: () => library.accent,
    overview: () => 'var(--accent)',
    assistant: () => 'var(--accent)',
  }
  const accent = $derived(ACCENTS[app.section]())

  /**
   * The window's one keyboard handler.
   *
   * It used to be a switch over five modifier combinations here, and a
   * second `<svelte:window>` handler inside `CalendarView` for seven bare
   * letters. Both are now rows in `lib/shortcuts.svelte.ts`, which is the
   * only place a shortcut is declared and the only thing that decides
   * whether one applies -- so a help sheet has somewhere to read from, and
   * two apps cannot quietly claim the same key.
   */
  function onKeydown(e: KeyboardEvent) {
    app.touch()
    shortcuts.press(e)
  }

  /**
   * Write everything outstanding, then let the window close.
   *
   * The shell cancels the close and waits for this, because the old approach
   * -- firing the flushes from `beforeunload` and letting the close proceed
   * -- did not work. A flush is an async round trip to the backend and it
   * lost the race against teardown essentially every time, so up to
   * `AUTOSAVE_MS` of typing went with the window.
   *
   * Two attempts. The first failure is usually the transient kind -- a vault
   * that has just auto-locked, a database busy for a moment -- and the retry
   * costs milliseconds. If the second fails too the shell closes us on its
   * own timer regardless; the alternative is an app that refuses to quit.
   */
  onSaveAndClose(async () => {
    for (let attempt = 0; attempt < 2; attempt++) {
      await app.flushAll()
      if (!app.saveFailing) break
    }
    await api.readyToClose().catch(() => {})
  })

  // Still worth doing on a plain unload -- a dev reload, a webview the shell
  // did not close itself. It cannot be awaited here, so it is a second line
  // of defence behind the handshake above rather than the mechanism.
  function onBeforeUnload() {
    void app.flushAll()
  }
</script>

<!-- The three that mean somebody is there. `touch` defers the screen lock and,
     on the machine holding the vault, the timer that forgets its key -- so
     every app has to be able to say so, not just the two with an editor in
     them. A press and a wheel cover the apps nobody types prose into: dragging
     a block, working the board, reading down a long entry. -->
<svelte:window
  onkeydown={onKeydown}
  onpointerdown={() => app.touch()}
  onwheel={() => app.touch()}
  onbeforeunload={onBeforeUnload}
/>

<!-- Outside the screen switch on purpose: a notification is a fact about
     the application, so it has to arrive on the lock screen and the error
     screen too. Those are the moments something has gone wrong. -->
<Toasts />
<!-- Also outside the screen switch, and mounted once: there is one context
     menu in the window, and every list opens it through the store. -->
<ContextMenu />

<div class="app" style="--journal-accent: {accent}">
  {#if app.screen === 'loading'}
    <div class="boot"><div class="mark"><Logo size={40} tile /></div></div>
  {:else if app.screen === 'error'}
    <ErrorScreen />
  {:else if app.screen === 'setup'}
    <Setup />
  {:else if app.screen === 'locked'}
    <LockScreen />
  {:else}
    <!-- Above the panes, not inside one: a conflict or a read-only vault is
         a fact about the whole window, and it must be visible whichever app
         is open. -->
    <div class="shell">
      <Notices />
      <div class="panes">
        <AppBar />
        <Sidebar />
        {#if app.section === 'assistant'}
          <AssistantView />
        {:else if app.section === 'notes'}
          <NotesView />
        {:else if app.section === 'todo'}
          <TodoView />
        {:else if app.section === 'calendar'}
          <CalendarView />
        {:else if app.section === 'library'}
          <LibraryView />
        {:else if app.section === 'overview'}
          <OverviewView />
        {:else}
          <EntryList />
          <main class="main"><Editor /></main>
        {/if}
        <!-- Last in the row, so it is the right-hand rail whichever app is
             open: the assistant works on every one of them. -->
        {#if agent.open && agent.supported}
          <ChatPanel />
        {/if}

        <!-- The way in, in the corner of whatever is open.
             It was a button at the foot of the app bar, filed with Settings
             and Lock, and that put it with the vault's controls -- which is
             the one thing it is not. It acts on the app you are looking at,
             about the thing you have selected in it, so it belongs over that
             app rather than in the column that switches between them. In the
             corner it is also findable without being read: a round button in
             the bottom right of a window is the most conventional gesture in
             software, and this is the feature least likely to be discovered
             by anybody hunting through a bar of grey labels. -->
        {#if agent.supported && !agent.open}
          <button
            class="float"
            onclick={() => void agent.toggle()}
            title="Assistant (A)"
            aria-label="Open the assistant"
          >
            <Icon name="sparkle" size={20} weight={1.7} />
          </button>
        {/if}
      </div>
    </div>

    <!-- Over everything, and outside the pane switch: settings and the
         shortcut sheet are the window's, not any one app's. -->
    {#if panels.settings !== null}
      <SettingsDialog />
    {/if}
    {#if panels.shortcuts}
      <ShortcutsHelp />
    {/if}
    <!-- The palette draws its own scrim, and mounts unconditionally because
         its open state is the one thing a global hotkey can set from outside
         the window. -->
    <Palette />

    <!-- What has been pressed, while a sequence is half finished. Small, in
         the corner, and gone in a second: without it, `g` is a keystroke
         that appears to have done nothing. -->
    {#if shortcuts.pending.length > 0}
      <div class="chord" aria-hidden="true">
        {shortcuts.pending.join(' ').toUpperCase()} …
      </div>
    {/if}
  {/if}
</div>

<style>
  .app {
    height: 100%;
  }
  /* A column, so a notice takes the height it needs and the panes take the
     rest -- rather than the notice overlaying the interface or pushing it
     off the bottom of the window. */
  .shell {
    display: flex;
    flex-direction: column;
    height: 100%;
  }
  .panes {
    display: flex;
    flex: 1;
    min-height: 0;
  }
  .main {
    flex: 1;
    min-width: 0;
  }

  /* Above the panes and clear of the window's edge. `fixed` rather than
     absolute inside a pane: every one of the panes scrolls, and a button that
     scrolled away with the list would not be the standing offer it is meant
     to be. */
  .float {
    position: fixed;
    right: var(--sp-6);
    bottom: var(--sp-6);
    z-index: 30;
    display: grid;
    place-items: center;
    width: var(--fab-size);
    height: var(--fab-size);
    border-radius: 50%;
    background: var(--journal-accent, var(--accent));
    color: var(--fg-on-accent);
    box-shadow: var(--shadow-lg);
    transition:
      scale var(--fast) var(--ease),
      box-shadow var(--fast) var(--ease);
  }
  .float:hover {
    scale: 1.06;
  }
  .float:active {
    scale: 0.97;
  }

  .chord {
    position: fixed;
    left: 50%;
    bottom: var(--sp-8);
    translate: -50% 0;
    z-index: 50;
    padding: var(--sp-2) var(--sp-4);
    border-radius: 999px;
    background: var(--bg-raised);
    border: 1px solid var(--border);
    box-shadow: var(--shadow);
    font-size: var(--text-sm);
    font-weight: 650;
    letter-spacing: 0.08em;
    color: var(--fg-muted);
  }

  .boot {
    display: grid;
    place-items: center;
    height: 100%;
    background: var(--bg);
  }
  .mark {
    animation: pulse 1.6s ease-in-out infinite;
  }
  @keyframes pulse {
    0%,
    100% {
      opacity: 0.3;
    }
    50% {
      opacity: 1;
    }
  }
</style>
