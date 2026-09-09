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
  // every session is as wrong as closing it every session.
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

  function onKeydown(e: KeyboardEvent) {
    const mod = e.metaKey || e.ctrlKey
    if (!mod) return
    switch (e.key.toLowerCase()) {
      case 'n':
        // The same key in all four apps, meaning the same thing: start the
        // next thing. In the journal that is a new entry; in the todo app and
        // the library it is the capture line; in the calendar it is an hour
        // set aside now.
        if (app.screen !== 'main') break
        e.preventDefault()
        if (app.section === 'todo') todo.focusCapture()
        else if (app.section === 'calendar') void calendar.bookNow()
        else if (app.section === 'library') library.focusCapture()
        else void app.newEntry()
        break
      case 'l':
        if (app.screen === 'main') {
          e.preventDefault()
          void app.lock()
        }
        break
      case 'f':
        if (app.screen === 'main') {
          e.preventDefault()
          document.querySelector<HTMLInputElement>('input[type="search"]')?.focus()
        }
        break
      case 'j':
        // Cycle through the apps without reaching for the sidebar. Four of
        // them now, so it steps rather than toggles, skipping any the open
        // vault's backend cannot offer.
        if (app.screen === 'main') {
          e.preventDefault()
          app.nextSection()
        }
        break
      case 's':
        // The app autosaves; honouring Ctrl+S anyway is politeness toward
        // the muscle memory of everyone who has ever lost work.
        e.preventDefault()
        void app.flush()
        void todo.flush()
        void calendar.flush()
        void library.flush()
        break
    }
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
      </div>
    </div>
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
