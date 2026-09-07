<script lang="ts">
  import { app } from './lib/state.svelte'
  import { todo } from './lib/todo.svelte'
  import { calendar } from './lib/calendar.svelte'
  import Sidebar from './components/Sidebar.svelte'
  import EntryList from './components/EntryList.svelte'
  import Editor from './components/Editor.svelte'
  import TodoView from './components/TodoView.svelte'
  import CalendarView from './components/CalendarView.svelte'
  import LockScreen from './components/LockScreen.svelte'
  import Setup from './components/Setup.svelte'
  import ErrorScreen from './components/ErrorScreen.svelte'
  import Logo from './components/Logo.svelte'

  void app.start()

  let todoView = $state<ReturnType<typeof TodoView> | null>(null)

  // Each app tints the window with the accent of whatever it has selected:
  // the journal you are in, the project you are looking at, or -- since the
  // calendar spans every project at once -- the vault's own accent.
  const accent = $derived(
    app.section === 'todo' ? todo.accent : app.section === 'calendar' ? 'var(--accent)' : app.accent,
  )

  function onKeydown(e: KeyboardEvent) {
    const mod = e.metaKey || e.ctrlKey
    if (!mod) return
    switch (e.key.toLowerCase()) {
      case 'n':
        // The same key in all three apps, meaning the same thing: start the
        // next thing. In the journal that is a new entry; in the todo app it
        // is the capture line; in the calendar it is an hour set aside now.
        if (app.screen !== 'main') break
        e.preventDefault()
        if (app.section === 'todo') todoView?.focusCapture()
        else if (app.section === 'calendar') void calendar.bookNow()
        else void app.newEntry()
        break
      case 'l':
        if (app.screen === 'main') { e.preventDefault(); void app.lock() }
        break
      case 'f':
        if (app.screen === 'main') {
          e.preventDefault()
          document.querySelector<HTMLInputElement>('input[type="search"]')?.focus()
        }
        break
      case 'j':
        // Cycle through the apps without reaching for the sidebar. Three of
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
        break
    }
  }

  // Persist in-flight edits if the window goes away.
  function onBeforeUnload() { void app.flush(); void todo.flush(); void calendar.flush() }
</script>

<svelte:window onkeydown={onKeydown} onbeforeunload={onBeforeUnload} />

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
    <div class="panes">
      <Sidebar />
      {#if app.section === 'todo'}
        <TodoView bind:this={todoView} />
      {:else if app.section === 'calendar'}
        <CalendarView />
      {:else}
        <EntryList />
        <main class="main"><Editor /></main>
      {/if}
    </div>
  {/if}
</div>

<style>
  .app { height: 100%; }
  .panes { display: flex; height: 100%; }
  .main { flex: 1; min-width: 0; }

  .boot { display: grid; place-items: center; height: 100%; background: var(--bg); }
  .mark { animation: pulse 1.6s ease-in-out infinite; }
  @keyframes pulse { 0%, 100% { opacity: 0.3; } 50% { opacity: 1; } }
</style>
