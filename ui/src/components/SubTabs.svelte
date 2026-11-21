<script lang="ts" generics="T extends string">
  // The tabs inside one settings page or dialog: Models' kinds of model, an
  // account's General / Mail / Servers, the vault's Security / Sharing.
  //
  // A page that stacked every section one under another grew taller than the
  // window, and the Save at the end of it ended up somewhere in the middle of
  // the scroll -- a section that saved on its own sat under it. Splitting a
  // page into a handful of tabs keeps each one short enough to read at once,
  // and leaves the page's Save at its foot.
  //
  // Drawn as the app's own chips -- app.css's `.filters` -- so it reads as the
  // same control the Todo and Mail lists filter with, rather than a third
  // kind of tab. Left and right arrows move between them, the way a tab list
  // is expected to.

  let {
    tabs,
    value = $bindable(),
    label,
  }: {
    tabs: { id: T; label: string; count?: number }[]
    value: T
    label: string
  } = $props()

  let list = $state<HTMLDivElement | null>(null)

  function onkeydown(e: KeyboardEvent) {
    if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return
    const at = Math.max(
      0,
      tabs.findIndex((t) => t.id === value),
    )
    const step = e.key === 'ArrowRight' ? 1 : -1
    const next = tabs[(at + step + tabs.length) % tabs.length]
    if (!next) return
    e.preventDefault()
    value = next.id
    list?.querySelector<HTMLButtonElement>(`[data-tab="${next.id}"]`)?.focus()
  }
</script>

<div
  class="filters subtabs"
  role="tablist"
  aria-label={label}
  bind:this={list}
  tabindex="-1"
  {onkeydown}
>
  {#each tabs as t (t.id)}
    <button
      class="filter"
      class:on={value === t.id}
      role="tab"
      data-tab={t.id}
      aria-selected={value === t.id}
      tabindex={value === t.id ? 0 : -1}
      onclick={() => (value = t.id)}
    >
      {t.label}
      {#if t.count}<span class="n">{t.count}</span>{/if}
    </button>
  {/each}
</div>

<style>
  /* Left-aligned under the page's heading, not centred the way a list's
     filters are over the list: these are the page's own sections. */
  .subtabs {
    width: fit-content;
    max-width: 100%;
    justify-content: flex-start;
    margin-bottom: var(--sp-6);
  }
</style>
