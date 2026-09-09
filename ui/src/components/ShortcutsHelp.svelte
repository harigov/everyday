<script lang="ts">
  // Every shortcut that applies right now, on one sheet.
  //
  // "Right now" is the part that matters. A cheat sheet listing the whole
  // table would show the calendar's `d`, `w` and `m` to somebody in the
  // library, where they do nothing, and would offer the todo app on a vault
  // whose backend has no tasks. `applicableBindings` asks each binding's
  // `when` and leaves out the ones that answer no, so the sheet is a true
  // statement about the window it is over.

  import { keysLabel } from '../lib/keys'
  import { panels } from '../lib/panels.svelte'
  import { applicableBindings, spellings } from '../lib/shortcuts.svelte'
  import { trapFocus } from '../lib/focus'
  import Icon from './Icon.svelte'

  const mac = navigator.userAgent.includes('Mac')
  // Read once, when the sheet opens: the bindings' `when` closures read the
  // stores, and nothing behind a modal is moving.
  const groups = applicableBindings()
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape') panels.shortcuts = false
  }}
/>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={() => (panels.shortcuts = false)}></div>
<div class="sheet" role="dialog" aria-modal="true" aria-label="Keyboard shortcuts" use:trapFocus>
  <header>
    <h2>Keyboard</h2>
    <button class="ghost" onclick={() => (panels.shortcuts = false)} title="Close">
      <Icon name="close" size={16} />
    </button>
  </header>

  <div class="body scroll">
    {#each groups as group (group.group)}
      <section>
        <span class="eyebrow">{group.group}</span>
        {#each group.items as binding (binding.label)}
          <div class="line">
            <span class="what">{binding.label}</span>
            <span class="keys">
              {#each spellings(binding) as spelling, i (spelling)}
                {#if i > 0}<span class="or">or</span>{/if}
                <span class="combo">
                  {#each keysLabel(spelling, mac).split(' then ') as part, n (n)}
                    {#if n > 0}<span class="then">then</span>{/if}
                    <kbd>{part}</kbd>
                  {/each}
                </span>
              {/each}
            </span>
          </div>
        {/each}
      </section>
    {/each}
  </div>

  <footer>
    <span class="hint">A letter is a letter while you are typing in a field.</span>
    <kbd>?</kbd>
  </footer>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 62;
    background: rgb(0 0 0 / 0.28);
  }
  .sheet {
    position: fixed;
    z-index: 63;
    top: 50%;
    left: 50%;
    translate: -50% -50%;
    display: flex;
    flex-direction: column;
    width: min(620px, calc(100vw - var(--sp-8)));
    max-height: min(680px, calc(100vh - var(--sp-8)));
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-raised);
    box-shadow: var(--shadow-lg);
    overflow: hidden;
  }

  header,
  footer {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    flex: none;
    padding: var(--sp-3) var(--sp-3) var(--sp-3) var(--sp-5);
  }
  header {
    border-bottom: 1px solid var(--border);
  }
  footer {
    padding: var(--sp-3) var(--sp-5);
    border-top: 1px solid var(--border);
  }
  h2 {
    flex: 1;
    margin: 0;
    font-size: var(--text-md);
    font-weight: 650;
  }
  .hint {
    flex: 1;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .ghost {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: var(--radius-sm);
    color: var(--fg-subtle);
  }
  .ghost:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .body {
    flex: 1;
    min-height: 0;
    padding: var(--sp-4) var(--sp-5) var(--sp-6);
    /* Two columns where there is room. The groups are short and the sheet is
       read by scanning, so a single tall column costs a scroll for nothing. */
    columns: 2;
    column-gap: var(--sp-8);
  }
  section {
    /* A group is not split across the fold of the columns: a heading at the
       bottom of one with its rows at the top of the next is unreadable. */
    break-inside: avoid;
    display: block;
    margin-bottom: var(--sp-5);
  }
  .eyebrow {
    display: block;
    margin-bottom: var(--sp-2);
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }

  .line {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--sp-3);
    padding: 3px 0;
  }
  .what {
    font-size: var(--text-sm);
    color: var(--fg-muted);
    min-width: 0;
  }
  .keys {
    display: flex;
    align-items: baseline;
    gap: 4px;
    flex: none;
  }
  .combo {
    display: flex;
    align-items: baseline;
    gap: 4px;
  }
  .then,
  .or {
    font-size: 10px;
    color: var(--fg-faint);
  }

  kbd {
    font-family: var(--font-ui);
    font-size: var(--text-xs);
    font-weight: 600;
    min-width: 20px;
    text-align: center;
    padding: 2px 6px;
    border: 1px solid var(--border);
    border-bottom-width: 2px;
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
    color: var(--fg-muted);
  }
</style>
