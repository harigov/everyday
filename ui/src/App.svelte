<script lang="ts">
  import { app } from './lib/state.svelte'
  import Sidebar from './components/Sidebar.svelte'
  import EntryList from './components/EntryList.svelte'
  import Editor from './components/Editor.svelte'
  import LockScreen from './components/LockScreen.svelte'
  import Setup from './components/Setup.svelte'
  import ErrorScreen from './components/ErrorScreen.svelte'
  import Logo from './components/Logo.svelte'

  void app.start()

  function onKeydown(e: KeyboardEvent) {
    const mod = e.metaKey || e.ctrlKey
    if (!mod) return
    switch (e.key.toLowerCase()) {
      case 'n':
        if (app.screen === 'main') { e.preventDefault(); void app.newEntry() }
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
      case 's':
        // The app autosaves; honouring Ctrl+S anyway is politeness toward
        // the muscle memory of everyone who has ever lost work.
        e.preventDefault()
        void app.flush()
        break
    }
  }

  // Persist in-flight edits if the window goes away.
  function onBeforeUnload() { void app.flush() }
</script>

<svelte:window onkeydown={onKeydown} onbeforeunload={onBeforeUnload} />

<div class="app" style="--journal-accent: {app.accent}">
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
      <EntryList />
      <main class="main"><Editor /></main>
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
