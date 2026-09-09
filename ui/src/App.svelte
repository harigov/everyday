<script lang="ts">
  import { api, onSaveAndClose } from './lib/api'
  import { app } from './lib/state.svelte'
  import { menu } from './lib/menu.svelte'
  import { notify } from './lib/notify.svelte'
  import { todo } from './lib/todo.svelte'
  import { calendar } from './lib/calendar.svelte'
  import { library } from './lib/library.svelte'
  import { tray } from './lib/tray.svelte'
  import { agent } from './lib/agent.svelte'
  import { panels } from './lib/panels.svelte'
  import { shortcuts } from './lib/shortcuts.svelte'
  import AppBar from './components/AppBar.svelte'
  import Sidebar from './components/Sidebar.svelte'
  import EntryList from './components/EntryList.svelte'
  import Editor from './components/Editor.svelte'
  import TodoView from './components/TodoView.svelte'
  import CalendarView from './components/CalendarView.svelte'
  import LibraryView from './components/LibraryView.svelte'
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

  // Put the quick actions in the menu bar, and keep them in step with the
  // vault from here on. The four apps have already registered what they
  // offer by the time this runs -- the imports above are what does it.
  tray.start()

  // Whether the assistant's rail was showing is remembered across launches:
  // it is a panel somebody either works with or does not, and reopening it
  // every session is as wrong as closing it every session. What it holds is
  // loaded by the panel itself -- see `ensureLoaded`, which also covers the
  // rail still being open after a lock cleared everything behind it.
  agent.restore()

  // Each app tints the window with the accent of whatever it has selected:
  // the journal you are in, the project you are looking at, the shelf you are
  // browsing, or -- since the calendar spans every project at once -- the
  // vault's own accent.
  const accent = $derived(
    app.section === 'todo'
      ? todo.accent
      : app.section === 'calendar'
        ? 'var(--accent)'
        : app.section === 'library'
          ? library.accent
          : app.accent,
  )

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
      await Promise.allSettled([app.flush(), todo.flush(), calendar.flush(), library.flush()])
      if (!app.saveFailing) break
    }
    await api.readyToClose().catch(() => {})
  })

  // Still worth doing on a plain unload -- a dev reload, a webview the shell
  // did not close itself. It cannot be awaited here, so it is a second line
  // of defence behind the handshake above rather than the mechanism.
  function onBeforeUnload() {
    void app.flush()
    void todo.flush()
    void calendar.flush()
    void library.flush()
  }
</script>

<svelte:window onkeydown={onKeydown} onbeforeunload={onBeforeUnload} />

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
         a fact about the whole window, and it must be visible whichever of
         the four apps is open. -->
    <div class="shell">
      <Notices />
      <div class="panes">
        <AppBar />
        <Sidebar />
        {#if app.section === 'todo'}
          <TodoView />
        {:else if app.section === 'calendar'}
          <CalendarView />
        {:else if app.section === 'library'}
          <LibraryView />
        {:else}
          <EntryList />
          <main class="main"><Editor /></main>
        {/if}
        <!-- Last in the row, so it is the right-hand rail whichever app is
             open: the assistant works on all four. -->
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
     absolute inside a pane: every one of the four scrolls, and a button that
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
