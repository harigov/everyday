<script lang="ts">
  // The themes, each as a small picture of the window drawn in it.
  //
  // The pictures are not screenshots and carry no colours of their own: each
  // is a few boxes inside an element with its own `data-theme`, so they are
  // drawn from the same tokens as the window itself (see `themes.ts`), and a
  // theme added to `themes.css` and `THEMES` appears here without anything
  // in this file changing.
  import { app } from '../lib/state.svelte'
  import { THEMES, type ThemeChoice, type ThemeId } from '../lib/themes'
  import Icon from './Icon.svelte'

  const light = THEMES.filter((t) => t.scheme === 'light')
  const dark = THEMES.filter((t) => t.scheme === 'dark')

  /** In the order they are drawn, which is the order the arrow keys move in. */
  const ORDER: ThemeChoice[] = ['system', ...light.map((t) => t.id), ...dark.map((t) => t.id)]

  let group = $state<HTMLElement>()

  function choose(choice: ThemeChoice) {
    app.setTheme(choice)
  }

  // A radio group, as it announces itself: one tab stop -- the chosen card --
  // and the arrow keys both move and choose, wrapping at the ends. Up and
  // Down move a whole row of the grid, so the keys go where the eye does.
  function onkeydown(e: KeyboardEvent) {
    const cards = [...(group?.querySelectorAll<HTMLElement>('[role="radio"]') ?? [])]
    const at = ORDER.indexOf(app.theme)
    const across = Math.max(1, Math.round(group!.clientWidth / (cards[0]?.offsetWidth ?? 1)))
    const step: Record<string, number> = {
      ArrowRight: 1,
      ArrowLeft: -1,
      ArrowDown: across,
      ArrowUp: -across,
    }
    let next: number
    if (e.key in step) next = (at + step[e.key]! + ORDER.length) % ORDER.length
    else if (e.key === 'Home') next = 0
    else if (e.key === 'End') next = ORDER.length - 1
    else return
    e.preventDefault()
    choose(ORDER[next]!)
    cards[next]?.focus()
  }
</script>

{#snippet mini(id: ThemeId)}
  <span class="mini" data-theme={id} aria-hidden="true">
    <span class="m-top"></span>
    <span class="m-row">
      <span class="m-bar">
        <i></i><i class="sel"></i><i></i>
      </span>
      <span class="m-side">
        <i class="sel"></i><i></i><i></i>
      </span>
      <span class="m-stage">
        <b></b>
        <i></i><i class="short"></i>
        <span class="m-card"><i></i><i class="short"></i></span>
        <span class="m-btn"></span>
      </span>
    </span>
  </span>
{/snippet}

{#snippet card(id: ThemeId, name: string, glass: boolean)}
  <button
    class="theme"
    class:on={app.theme === id}
    role="radio"
    aria-checked={app.theme === id}
    tabindex={app.theme === id ? 0 : -1}
    onclick={() => choose(id)}
  >
    {@render mini(id)}
    <span class="label">
      <span class="name">{name}</span>
      {#if glass}<span class="kind">Glass</span>{/if}
      {#if app.theme === id}<span class="tick"><Icon name="check" size={13} /></span>{/if}
    </span>
  </button>
{/snippet}

<div
  class="picker"
  role="radiogroup"
  aria-label="Theme"
  tabindex="-1"
  bind:this={group}
  {onkeydown}
>
  <button
    class="theme"
    class:on={app.theme === 'system'}
    role="radio"
    aria-checked={app.theme === 'system'}
    tabindex={app.theme === 'system' ? 0 : -1}
    onclick={() => choose('system')}
  >
    <span class="split">
      {@render mini('light')}
      {@render mini('dark')}
    </span>
    <span class="label">
      <span class="name">Match desktop</span>
      {#if app.theme === 'system'}<span class="tick"><Icon name="check" size={13} /></span>{/if}
    </span>
  </button>
  {#each light as t (t.id)}
    {@render card(t.id, t.name, t.glass)}
  {/each}
  {#each dark as t (t.id)}
    {@render card(t.id, t.name, t.glass)}
  {/each}
</div>
<p class="hint">
  Match desktop follows the system between Paper and Ink. Glass themes float the sidebar and the
  page as frosted cards over a coloured backdrop.
</p>

<style>
  .picker {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
    gap: var(--sp-3);
  }

  .theme {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: 6px;
    border-radius: var(--radius-lg);
    border: 1px solid var(--border);
    background: var(--bg-raised);
    text-align: left;
    transition:
      border-color var(--fast) var(--ease),
      box-shadow var(--fast) var(--ease),
      translate var(--fast) var(--ease);
  }
  .theme:hover {
    border-color: var(--border-strong);
    translate: 0 -1px;
    box-shadow: var(--shadow);
  }
  .theme.on {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 22%, transparent);
  }

  .label {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 20px;
    padding: 0 4px 2px;
  }
  .name {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
  }
  .kind {
    font-size: 10px;
    font-weight: 650;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }
  .tick {
    display: flex;
    margin-left: auto;
    color: var(--accent);
  }

  /* Paper on the left, Ink on the right: the same picture twice, one over the
     other, the upper cut away down the middle. */
  .split {
    display: grid;
  }
  .split .mini {
    grid-area: 1 / 1;
  }
  .split .mini:last-child {
    clip-path: inset(0 0 0 50%);
  }

  /* The window, at about a sixth of its size: a strip across the top, the app
     bar, the sidebar and the stage, spaced and rounded by the theme's own
     frame tokens scaled down with it. */
  .mini {
    display: flex;
    flex-direction: column;
    height: 92px;
    overflow: hidden;
    border-radius: var(--radius);
    background: var(--backdrop);
    color: var(--fg);
    --g: calc(var(--shell-gap) / 3);
    --r: calc(var(--shell-radius) / 3);
  }
  .m-top {
    flex: none;
    height: 9px;
    background: var(--chrome);
    border-bottom: var(--shell-rule);
  }
  .m-row {
    display: flex;
    flex: 1;
    min-height: 0;
    gap: var(--g);
    padding: 0 var(--g) var(--g) 0;
  }
  .m-bar {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
    width: 14px;
    padding-top: 5px;
    background: var(--chrome);
    border-right: var(--shell-rule);
  }
  .m-side {
    display: flex;
    flex-direction: column;
    gap: 4px;
    width: 34px;
    padding: 6px 4px;
    background: var(--sidebar);
    border-right: var(--shell-rule);
    border-radius: var(--r);
    box-shadow: var(--edge);
  }
  .m-stage {
    position: relative;
    display: flex;
    flex: 1;
    min-width: 0;
    flex-direction: column;
    gap: 4px;
    padding: 7px 8px;
    background: var(--surface);
    border-radius: var(--r);
    box-shadow: var(--edge), var(--shadow-sm);
  }

  .mini i {
    display: block;
    height: 3px;
    border-radius: 2px;
    background: var(--fg-faint);
    opacity: 0.6;
  }
  .mini i.short {
    width: 60%;
  }
  .m-bar i {
    width: 6px;
    height: 6px;
    border-radius: 2px;
  }
  .m-bar i.sel,
  .m-side i.sel {
    background: var(--accent);
    opacity: 1;
  }
  .m-side i {
    height: 4px;
  }
  .m-stage b {
    display: block;
    width: 45%;
    height: 5px;
    border-radius: 2px;
    background: var(--fg);
  }
  .m-card {
    display: flex;
    flex-direction: column;
    gap: 3px;
    margin-top: 2px;
    padding: 4px;
    border-radius: 3px;
    background: var(--bg-raised);
    border: 1px solid var(--border);
  }
  .m-btn {
    position: absolute;
    right: 6px;
    bottom: 6px;
    width: 18px;
    height: 7px;
    border-radius: 99px;
    background: var(--accent);
  }
</style>
